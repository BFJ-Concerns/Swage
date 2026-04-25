//! Tests for HookRegistration and HookRegistry dispatch engine.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;
use serde_json::json;
use swage_hook::{FailureMode, Hook, HookEvent, HookOutcome, HookRegistration, HookRegistry};
use tokio::sync::Mutex;

// -- Test hooks -------------------------------------------------------------

/// Records its name each time it executes. Always returns Continue.
struct RecordingHook {
    hook_name: String,
    log: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Hook for RecordingHook {
    fn name(&self) -> &str {
        &self.hook_name
    }

    async fn execute(&self, _event: &HookEvent) -> HookOutcome {
        self.log.lock().await.push(self.hook_name.clone());
        HookOutcome::Continue
    }
}

/// Always blocks with a fixed reason.
struct BlockHook {
    hook_name: String,
    reason: String,
    log: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Hook for BlockHook {
    fn name(&self) -> &str {
        &self.hook_name
    }

    async fn execute(&self, _event: &HookEvent) -> HookOutcome {
        self.log.lock().await.push(self.hook_name.clone());
        HookOutcome::Block(self.reason.clone())
    }
}

/// Modifies the event by replacing content with uppercase.
struct ModifyHook {
    hook_name: String,
}

#[async_trait]
impl Hook for ModifyHook {
    fn name(&self) -> &str {
        &self.hook_name
    }

    async fn execute(&self, event: &HookEvent) -> HookOutcome {
        match event {
            HookEvent::BeforeInbound { content } => {
                HookOutcome::ContinueModified(Box::new(HookEvent::BeforeInbound {
                    content: content.to_uppercase(),
                }))
            }
            _ => HookOutcome::Continue,
        }
    }
}

/// Always returns RequestApproval with a configurable prompt.
struct ApprovalHook {
    hook_name: String,
    prompt: String,
}

#[async_trait]
impl Hook for ApprovalHook {
    fn name(&self) -> &str {
        &self.hook_name
    }

    async fn execute(&self, _event: &HookEvent) -> HookOutcome {
        HookOutcome::RequestApproval {
            prompt: self.prompt.clone(),
            context: None,
            timeout: None,
        }
    }
}

/// Sleeps for a given duration before returning. Used for timeout tests.
struct SlowHook {
    hook_name: String,
    delay: Duration,
}

#[async_trait]
impl Hook for SlowHook {
    fn name(&self) -> &str {
        &self.hook_name
    }

    async fn execute(&self, _event: &HookEvent) -> HookOutcome {
        tokio::time::sleep(self.delay).await;
        HookOutcome::Continue
    }
}

/// Records the event content it receives. Used to verify ContinueModified propagation.
struct ContentCapture {
    hook_name: String,
    captured: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Hook for ContentCapture {
    fn name(&self) -> &str {
        &self.hook_name
    }

    async fn execute(&self, event: &HookEvent) -> HookOutcome {
        if let HookEvent::BeforeInbound { content } = event {
            self.captured.lock().await.push(content.clone());
        }
        HookOutcome::Continue
    }
}

// -- Helper to build a registration -----------------------------------------

fn registration(hook: Arc<dyn Hook>, priority: i32) -> HookRegistration {
    HookRegistration::new(hook).with_priority(priority)
}

fn registration_with_filter(
    hook: Arc<dyn Hook>,
    priority: i32,
    event_filter: fn(&HookEvent) -> bool,
) -> HookRegistration {
    HookRegistration::new(hook)
        .with_priority(priority)
        .with_event_filter(event_filter)
}

fn registration_with_matcher(
    hook: Arc<dyn Hook>,
    priority: i32,
    matcher: Regex,
) -> HookRegistration {
    HookRegistration::new(hook)
        .with_priority(priority)
        .with_matcher(matcher)
}

// -- Priority ordering tests ------------------------------------------------

#[tokio::test]
async fn dispatch_executes_hooks_in_priority_order() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut registry = HookRegistry::new();

    // Register in reverse priority order to prove sorting works
    registry.register(registration(
        Arc::new(RecordingHook {
            hook_name: "third".into(),
            log: log.clone(),
        }),
        30,
    ));
    registry.register(registration(
        Arc::new(RecordingHook {
            hook_name: "first".into(),
            log: log.clone(),
        }),
        10,
    ));
    registry.register(registration(
        Arc::new(RecordingHook {
            hook_name: "second".into(),
            log: log.clone(),
        }),
        20,
    ));

    let event = HookEvent::SessionStart {
        session_id: "s".into(),
    };
    registry.dispatch(event).await;

    let order = log.lock().await;
    assert_eq!(*order, vec!["first", "second", "third"]);
}

#[tokio::test]
async fn dispatch_preserves_registration_order_for_equal_priority() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut registry = HookRegistry::new();

    // All have the same priority — registration order should be preserved
    for name in &["alpha", "beta", "gamma"] {
        registry.register(registration(
            Arc::new(RecordingHook {
                hook_name: name.to_string(),
                log: log.clone(),
            }),
            0,
        ));
    }

    let event = HookEvent::SessionStart {
        session_id: "s".into(),
    };
    registry.dispatch(event).await;

    let order = log.lock().await;
    assert_eq!(*order, vec!["alpha", "beta", "gamma"]);
}

// -- Block stops chain ------------------------------------------------------

#[tokio::test]
async fn block_stops_chain_and_returns_block() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut registry = HookRegistry::new();

    registry.register(registration(
        Arc::new(RecordingHook {
            hook_name: "before".into(),
            log: log.clone(),
        }),
        10,
    ));
    registry.register(registration(
        Arc::new(BlockHook {
            hook_name: "blocker".into(),
            reason: "denied".into(),
            log: log.clone(),
        }),
        20,
    ));
    registry.register(registration(
        Arc::new(RecordingHook {
            hook_name: "after".into(),
            log: log.clone(),
        }),
        30,
    ));

    let event = HookEvent::PreToolUse {
        tool_name: "Bash".into(),
        params: json!(null),
    };
    let outcome = registry.dispatch(event).await;

    // "after" should never execute
    let executed = log.lock().await;
    assert_eq!(*executed, vec!["before", "blocker"]);

    match outcome {
        HookOutcome::Block(reason) => assert_eq!(reason, "denied"),
        _ => panic!("expected Block outcome"),
    }
}

// -- ContinueModified propagation -------------------------------------------

#[tokio::test]
async fn continue_modified_propagates_to_subsequent_hooks() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let mut registry = HookRegistry::new();

    // First hook modifies the event (uppercases content)
    registry.register(registration(
        Arc::new(ModifyHook {
            hook_name: "modifier".into(),
        }),
        10,
    ));

    // Second hook captures the event it receives
    registry.register(registration(
        Arc::new(ContentCapture {
            hook_name: "capture".into(),
            captured: captured.clone(),
        }),
        20,
    ));

    let event = HookEvent::BeforeInbound {
        content: "hello world".into(),
    };
    registry.dispatch(event).await;

    let contents = captured.lock().await;
    assert_eq!(contents[0], "HELLO WORLD");
}

// -- Timeout with FailOpen --------------------------------------------------

#[tokio::test]
async fn timeout_with_fail_open_returns_continue() {
    let mut registry = HookRegistry::new();

    let slow_hook: Arc<dyn Hook> = Arc::new(SlowHook {
        hook_name: "slow".into(),
        delay: Duration::from_secs(5),
    });

    let mut reg = HookRegistration::new(slow_hook);
    reg.timeout = Duration::from_millis(50);
    reg.failure_mode = FailureMode::FailOpen;
    registry.register(reg);

    let event = HookEvent::SessionStart {
        session_id: "s".into(),
    };
    let outcome = registry.dispatch(event).await;

    assert!(
        matches!(outcome, HookOutcome::Continue),
        "FailOpen timeout should return Continue"
    );
}

// -- Timeout with FailClosed ------------------------------------------------

#[tokio::test]
async fn timeout_with_fail_closed_returns_block() {
    let mut registry = HookRegistry::new();

    let slow_hook: Arc<dyn Hook> = Arc::new(SlowHook {
        hook_name: "slow".into(),
        delay: Duration::from_secs(5),
    });

    let mut reg = HookRegistration::new(slow_hook);
    reg.timeout = Duration::from_millis(50);
    reg.failure_mode = FailureMode::FailClosed;
    registry.register(reg);

    let event = HookEvent::SessionStart {
        session_id: "s".into(),
    };
    let outcome = registry.dispatch(event).await;

    match outcome {
        HookOutcome::Block(reason) => {
            assert!(
                reason.contains("timeout"),
                "block reason should mention timeout, got: {reason}"
            );
        }
        _ => panic!("FailClosed timeout should return Block"),
    }
}

// -- Event filter -----------------------------------------------------------

#[tokio::test]
async fn event_filter_excludes_non_matching_hooks() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut registry = HookRegistry::new();

    // This hook only matches PreToolUse events
    registry.register(registration_with_filter(
        Arc::new(RecordingHook {
            hook_name: "tool-only".into(),
            log: log.clone(),
        }),
        0,
        |event| matches!(event, HookEvent::PreToolUse { .. }),
    ));

    // Dispatch a SessionStart — should not trigger
    let event = HookEvent::SessionStart {
        session_id: "s".into(),
    };
    registry.dispatch(event).await;

    let executed = log.lock().await;
    assert!(
        executed.is_empty(),
        "hook should not fire for non-matching event"
    );
}

#[tokio::test]
async fn event_filter_includes_matching_hooks() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut registry = HookRegistry::new();

    registry.register(registration_with_filter(
        Arc::new(RecordingHook {
            hook_name: "tool-only".into(),
            log: log.clone(),
        }),
        0,
        |event| matches!(event, HookEvent::PreToolUse { .. }),
    ));

    let event = HookEvent::PreToolUse {
        tool_name: "Bash".into(),
        params: json!(null),
    };
    registry.dispatch(event).await;

    let executed = log.lock().await;
    assert_eq!(*executed, vec!["tool-only"]);
}

// -- Regex matcher ----------------------------------------------------------

#[tokio::test]
async fn regex_matcher_filters_by_event_name() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut registry = HookRegistry::new();

    // Matcher only matches events whose name contains "Tool"
    registry.register(registration_with_matcher(
        Arc::new(RecordingHook {
            hook_name: "tool-watcher".into(),
            log: log.clone(),
        }),
        0,
        Regex::new("Tool").unwrap(),
    ));

    // SessionStart should not match
    registry
        .dispatch(HookEvent::SessionStart {
            session_id: "s".into(),
        })
        .await;
    assert!(log.lock().await.is_empty());

    // PreToolUse should match
    registry
        .dispatch(HookEvent::PreToolUse {
            tool_name: "Edit".into(),
            params: json!(null),
        })
        .await;
    assert_eq!(*log.lock().await, vec!["tool-watcher"]);
}

// -- Empty registry ---------------------------------------------------------

#[tokio::test]
async fn empty_registry_returns_continue() {
    let registry = HookRegistry::new();
    let event = HookEvent::SessionStart {
        session_id: "s".into(),
    };
    let outcome = registry.dispatch(event).await;
    assert!(matches!(outcome, HookOutcome::Continue));
}

// -- Default trait ----------------------------------------------------------

#[test]
fn hook_registry_default_creates_empty_registry() {
    let registry = HookRegistry::default();
    // Just verify it compiles and creates without panic
    drop(registry);
}

// -- RequestApproval dispatch priority --------------------------------------

#[tokio::test]
async fn approval_returned_when_single_hook_requests_it() {
    // A single hook requesting approval produces RequestApproval from dispatch.
    let mut registry = HookRegistry::new();
    registry.register(registration(
        Arc::new(ApprovalHook {
            hook_name: "approver".into(),
            prompt: "please review".into(),
        }),
        0,
    ));

    let event = HookEvent::PreToolUse {
        tool_name: "Bash".into(),
        params: json!(null),
    };
    let outcome = registry.dispatch(event).await;

    match outcome {
        HookOutcome::RequestApproval { prompt, .. } => {
            assert_eq!(prompt, "please review");
        }
        _ => panic!("expected RequestApproval, got {outcome:?}"),
    }
}

#[tokio::test]
async fn approval_with_continue_hooks_returns_approval() {
    // Approval hook + continue hook → approval wins over continue.
    let mut registry = HookRegistry::new();

    registry.register(registration(
        Arc::new(ApprovalHook {
            hook_name: "approver".into(),
            prompt: "needs review".into(),
        }),
        10,
    ));
    registry.register(registration(
        Arc::new(RecordingHook {
            hook_name: "continuer".into(),
            log: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        }),
        20,
    ));

    let event = HookEvent::SessionStart {
        session_id: "s".into(),
    };
    let outcome = registry.dispatch(event).await;

    match outcome {
        HookOutcome::RequestApproval { prompt, .. } => {
            assert_eq!(prompt, "needs review");
        }
        _ => panic!("expected RequestApproval, got {outcome:?}"),
    }
}

#[tokio::test]
async fn block_overrides_approval_regardless_of_order() {
    // Block always takes precedence over RequestApproval, even when approval
    // runs first (higher priority = lower number).
    let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let mut registry = HookRegistry::new();

    // Approval hook runs first (priority 10)
    registry.register(registration(
        Arc::new(ApprovalHook {
            hook_name: "approver".into(),
            prompt: "should be ignored".into(),
        }),
        10,
    ));
    // Block hook runs second (priority 20) — Block should still win
    registry.register(registration(
        Arc::new(BlockHook {
            hook_name: "blocker".into(),
            reason: "hard block".into(),
            log: log.clone(),
        }),
        20,
    ));

    let event = HookEvent::PreToolUse {
        tool_name: "Bash".into(),
        params: json!(null),
    };
    let outcome = registry.dispatch(event).await;

    match outcome {
        HookOutcome::Block(reason) => assert_eq!(reason, "hard block"),
        _ => panic!("expected Block, got {outcome:?}"),
    }
}

#[tokio::test]
async fn first_approval_wins_among_multiple_approvals() {
    // When multiple hooks request approval, the highest-priority one (lowest
    // priority number, runs first) owns the resulting outcome.
    let mut registry = HookRegistry::new();

    registry.register(registration(
        Arc::new(ApprovalHook {
            hook_name: "first-approver".into(),
            prompt: "first prompt".into(),
        }),
        10,
    ));
    registry.register(registration(
        Arc::new(ApprovalHook {
            hook_name: "second-approver".into(),
            prompt: "second prompt".into(),
        }),
        20,
    ));

    let event = HookEvent::SessionStart {
        session_id: "s".into(),
    };
    let outcome = registry.dispatch(event).await;

    match outcome {
        HookOutcome::RequestApproval { prompt, .. } => {
            assert_eq!(
                prompt, "first prompt",
                "first (highest-priority) approval should win"
            );
        }
        _ => panic!("expected RequestApproval, got {outcome:?}"),
    }
}
