//! Tests for the hook type system: Hook trait, HookEvent, and HookOutcome.

use async_trait::async_trait;
use serde_json::json;
use swage_hook::{FailureMode, Hook, HookEvent, HookOutcome};

// -- Mock hook for testing --------------------------------------------------

/// A simple hook that always returns Continue, used to verify trait mechanics.
struct AlwaysContinueHook;

#[async_trait]
impl Hook for AlwaysContinueHook {
    fn name(&self) -> &str {
        "always-continue"
    }

    async fn execute(&self, _event: &HookEvent) -> HookOutcome {
        HookOutcome::Continue
    }
}

/// A hook that blocks with a reason, used to verify Block outcome.
struct BlockingHook {
    reason: String,
}

#[async_trait]
impl Hook for BlockingHook {
    fn name(&self) -> &str {
        "blocker"
    }

    async fn execute(&self, _event: &HookEvent) -> HookOutcome {
        HookOutcome::Block(self.reason.clone())
    }
}

// -- Hook trait tests -------------------------------------------------------

#[tokio::test]
async fn hook_name_returns_identifier() {
    let hook = AlwaysContinueHook;
    assert_eq!(hook.name(), "always-continue");
}

#[tokio::test]
async fn hook_execute_returns_continue() {
    let hook = AlwaysContinueHook;
    let event = HookEvent::SessionStart {
        session_id: "test-session".into(),
    };
    let outcome = hook.execute(&event).await;
    assert!(matches!(outcome, HookOutcome::Continue));
}

#[tokio::test]
async fn hook_execute_returns_block_with_reason() {
    let hook = BlockingHook {
        reason: "not allowed".into(),
    };
    let event = HookEvent::PreToolUse {
        tool_name: "Bash".into(),
        params: json!({"command": "rm -rf /"}),
    };
    let outcome = hook.execute(&event).await;
    match outcome {
        HookOutcome::Block(reason) => assert_eq!(reason, "not allowed"),
        _ => panic!("expected Block outcome"),
    }
}

// -- HookEvent variant construction -----------------------------------------

#[test]
fn hook_event_session_start() {
    let event = HookEvent::SessionStart {
        session_id: "abc-123".into(),
    };
    assert_eq!(event.event_name(), "SessionStart");
}

#[test]
fn hook_event_session_end() {
    let event = HookEvent::SessionEnd {
        session_id: "abc-123".into(),
    };
    assert_eq!(event.event_name(), "SessionEnd");
}

#[test]
fn hook_event_pre_tool_use() {
    let event = HookEvent::PreToolUse {
        tool_name: "Edit".into(),
        params: json!({"file_path": "/tmp/test.rs"}),
    };
    assert_eq!(event.event_name(), "PreToolUse");
}

#[test]
fn hook_event_post_tool_use() {
    let event = HookEvent::PostToolUse {
        tool_name: "Read".into(),
        params: json!({}),
        result: json!({"content": "file contents"}),
    };
    assert_eq!(event.event_name(), "PostToolUse");
}

#[test]
fn hook_event_before_inbound() {
    let event = HookEvent::BeforeInbound {
        content: "user message".into(),
    };
    assert_eq!(event.event_name(), "BeforeInbound");
}

#[test]
fn hook_event_after_response() {
    let event = HookEvent::AfterResponse {
        content: "assistant response".into(),
    };
    assert_eq!(event.event_name(), "AfterResponse");
}

#[test]
fn hook_event_pre_compact() {
    let event = HookEvent::PreCompact {
        context: json!({"messages": []}),
    };
    assert_eq!(event.event_name(), "PreCompact");
}

#[test]
fn hook_event_agent_stop() {
    let event = HookEvent::AgentStop {
        agent_id: "agent-42".into(),
    };
    assert_eq!(event.event_name(), "AgentStop");
}

#[test]
fn hook_event_topology_start() {
    let event = HookEvent::TopologyStart {
        topology_run_id: "run-1".into(),
        team_id: "team-1".into(),
        session_id: "session-1".into(),
    };
    assert_eq!(event.event_name(), "TopologyStart");
}

#[test]
fn hook_event_topology_end() {
    let event = HookEvent::TopologyEnd {
        topology_run_id: "run-1".into(),
        team_id: "team-1".into(),
        session_id: "session-1".into(),
        success: true,
    };
    assert_eq!(event.event_name(), "TopologyEnd");
}

#[test]
fn hook_event_node_start() {
    let event = HookEvent::NodeStart {
        topology_run_id: "run-1".into(),
        node_execution_id: "exec-1".into(),
        node_id: "plan".into(),
        agent_id: Some("planner".into()),
    };
    assert_eq!(event.event_name(), "NodeStart");
}

#[test]
fn hook_event_node_end() {
    let event = HookEvent::NodeEnd {
        topology_run_id: "run-1".into(),
        node_execution_id: "exec-1".into(),
        node_id: "plan".into(),
        success: true,
        output: Some(json!({"result": "ok"})),
    };
    assert_eq!(event.event_name(), "NodeEnd");
}

#[test]
fn hook_event_fan_out_spawn() {
    let event = HookEvent::FanOutSpawn {
        topology_run_id: "run-1".into(),
        source_node_execution_id: "exec-1".into(),
        edge_id: "plan->review".into(),
        branch_count: 3,
    };
    assert_eq!(event.event_name(), "FanOutSpawn");
}

#[test]
fn hook_event_fan_out_collect() {
    let event = HookEvent::FanOutCollect {
        topology_run_id: "run-1".into(),
        source_node_execution_id: "exec-1".into(),
        edge_id: "plan->review".into(),
        branch_count: 3,
    };
    assert_eq!(event.event_name(), "FanOutCollect");
}

// -- HookEvent derives Debug and Clone --------------------------------------

#[test]
fn hook_event_is_clone() {
    let event = HookEvent::SessionStart {
        session_id: "s1".into(),
    };
    let cloned = event.clone();
    assert_eq!(cloned.event_name(), "SessionStart");
}

#[test]
fn hook_event_is_debug() {
    let event = HookEvent::AgentStop {
        agent_id: "a1".into(),
    };
    let debug_str = format!("{:?}", event);
    assert!(debug_str.contains("AgentStop"));
}

// -- HookOutcome variants ---------------------------------------------------

#[test]
fn hook_outcome_continue() {
    let outcome = HookOutcome::Continue;
    assert!(matches!(outcome, HookOutcome::Continue));
}

#[test]
fn hook_outcome_block() {
    let outcome = HookOutcome::Block("denied".into());
    match outcome {
        HookOutcome::Block(reason) => assert_eq!(reason, "denied"),
        _ => panic!("expected Block"),
    }
}

#[test]
fn hook_outcome_continue_modified() {
    let modified_event = HookEvent::BeforeInbound {
        content: "sanitised content".into(),
    };
    let outcome = HookOutcome::ContinueModified(Box::new(modified_event));
    match outcome {
        HookOutcome::ContinueModified(event) => {
            assert_eq!(event.event_name(), "BeforeInbound");
        }
        _ => panic!("expected ContinueModified"),
    }
}

// -- HookOutcome derives Debug and Clone ------------------------------------

#[test]
fn hook_outcome_is_clone() {
    let outcome = HookOutcome::Block("reason".into());
    let cloned = outcome.clone();
    assert!(matches!(cloned, HookOutcome::Block(_)));
}

#[test]
fn hook_outcome_is_debug() {
    let outcome = HookOutcome::Continue;
    let debug_str = format!("{:?}", outcome);
    assert!(debug_str.contains("Continue"));
}

// -- FailureMode ------------------------------------------------------------

#[test]
fn failure_mode_fail_open() {
    let mode = FailureMode::FailOpen;
    assert_eq!(mode, FailureMode::FailOpen);
}

#[test]
fn failure_mode_fail_closed() {
    let mode = FailureMode::FailClosed;
    assert_eq!(mode, FailureMode::FailClosed);
}

#[test]
fn failure_mode_is_copy() {
    let mode = FailureMode::FailOpen;
    let copied = mode;
    // Both still usable — mode is Copy
    assert_eq!(mode, copied);
}

#[test]
fn failure_mode_is_debug() {
    let debug_str = format!("{:?}", FailureMode::FailClosed);
    assert!(debug_str.contains("FailClosed"));
}

// -- HookOutcome::RequestApproval variant -----------------------------------

#[test]
fn hook_outcome_request_approval_minimal() {
    // Construct with just a prompt, no context or timeout override.
    let outcome = HookOutcome::RequestApproval {
        prompt: "Please review this tool call".into(),
        context: None,
        timeout: None,
    };
    match outcome {
        HookOutcome::RequestApproval {
            prompt,
            context,
            timeout,
        } => {
            assert_eq!(prompt, "Please review this tool call");
            assert!(context.is_none());
            assert!(timeout.is_none());
        }
        _ => panic!("expected RequestApproval"),
    }
}

#[test]
fn hook_outcome_request_approval_with_context() {
    // Context carries structured data for the human reviewer.
    let ctx = serde_json::json!({ "tool": "Bash", "command": "rm -rf /tmp/scratch" });
    let outcome = HookOutcome::RequestApproval {
        prompt: "Destructive command requires approval".into(),
        context: Some(ctx.clone()),
        timeout: None,
    };
    match outcome {
        HookOutcome::RequestApproval {
            context: Some(c), ..
        } => {
            assert_eq!(c["tool"], "Bash");
        }
        _ => panic!("expected RequestApproval with context"),
    }
}

#[test]
fn hook_outcome_request_approval_with_timeout() {
    // Timeout overrides the config-level HITL timeout for this specific request.
    let deadline = std::time::Duration::from_secs(300);
    let outcome = HookOutcome::RequestApproval {
        prompt: "Sensitive operation".into(),
        context: None,
        timeout: Some(deadline),
    };
    match outcome {
        HookOutcome::RequestApproval {
            timeout: Some(d), ..
        } => {
            assert_eq!(d.as_secs(), 300);
        }
        _ => panic!("expected RequestApproval with timeout"),
    }
}

#[test]
fn hook_outcome_request_approval_is_debug() {
    let outcome = HookOutcome::RequestApproval {
        prompt: "needs review".into(),
        context: None,
        timeout: None,
    };
    let debug_str = format!("{:?}", outcome);
    assert!(debug_str.contains("RequestApproval"));
    assert!(debug_str.contains("needs review"));
}

#[test]
fn hook_outcome_request_approval_is_clone() {
    let original = HookOutcome::RequestApproval {
        prompt: "clone me".into(),
        context: Some(serde_json::json!({"key": "value"})),
        timeout: Some(std::time::Duration::from_secs(60)),
    };
    let cloned = original.clone();
    match cloned {
        HookOutcome::RequestApproval {
            prompt,
            context,
            timeout,
        } => {
            assert_eq!(prompt, "clone me");
            assert!(context.is_some());
            assert_eq!(timeout.unwrap().as_secs(), 60);
        }
        _ => panic!("expected RequestApproval after clone"),
    }
}

// -- Hook trait is object-safe and Send + Sync ------------------------------

#[tokio::test]
async fn hook_trait_is_object_safe() {
    let hook: Box<dyn Hook> = Box::new(AlwaysContinueHook);
    assert_eq!(hook.name(), "always-continue");
}

#[tokio::test]
async fn hook_can_be_sent_across_threads() {
    let hook: Box<dyn Hook> = Box::new(AlwaysContinueHook);
    let handle = tokio::spawn(async move {
        let event = HookEvent::SessionStart {
            session_id: "threaded".into(),
        };
        hook.execute(&event).await
    });
    let outcome = handle.await.unwrap();
    assert!(matches!(outcome, HookOutcome::Continue));
}

// -- Message lifecycle event variants ---------------------------------------

#[test]
fn hook_event_message_send() {
    let event = HookEvent::MessageSend {
        sender_agent_id: "agent-a".into(),
        recipient_agent_id: "agent-b".into(),
        kind: "delegate".into(),
        payload: json!({"task": "summarise"}),
    };
    assert_eq!(event.event_name(), "MessageSend");
}

#[test]
fn hook_event_message_deliver() {
    let event = HookEvent::MessageDeliver {
        message_id: "msg-1".into(),
        sender_agent_id: "agent-a".into(),
        recipient_agent_id: "agent-b".into(),
        kind: "delegate".into(),
        payload: json!({"task": "summarise"}),
    };
    assert_eq!(event.event_name(), "MessageDeliver");
}

#[test]
fn hook_event_message_receive() {
    let event = HookEvent::MessageReceive {
        message_id: "msg-1".into(),
        sender_agent_id: "agent-a".into(),
        recipient_agent_id: "agent-b".into(),
        kind: "notify".into(),
        payload: json!({"info": "done"}),
    };
    assert_eq!(event.event_name(), "MessageReceive");
}

// -- Mock hook exercised with every event variant ---------------------------

#[tokio::test]
async fn mock_hook_handles_all_event_variants() {
    let hook = AlwaysContinueHook;

    let events = vec![
        HookEvent::SessionStart {
            session_id: "s".into(),
        },
        HookEvent::SessionEnd {
            session_id: "s".into(),
        },
        HookEvent::PreToolUse {
            tool_name: "t".into(),
            params: json!(null),
        },
        HookEvent::PostToolUse {
            tool_name: "t".into(),
            params: json!(null),
            result: json!(null),
        },
        HookEvent::BeforeInbound {
            content: "c".into(),
        },
        HookEvent::AfterResponse {
            content: "c".into(),
        },
        HookEvent::PreCompact {
            context: json!(null),
        },
        HookEvent::AgentStop {
            agent_id: "a".into(),
        },
        HookEvent::TopologyStart {
            topology_run_id: "run".into(),
            team_id: "team".into(),
            session_id: "session".into(),
        },
        HookEvent::TopologyEnd {
            topology_run_id: "run".into(),
            team_id: "team".into(),
            session_id: "session".into(),
            success: true,
        },
        HookEvent::NodeStart {
            topology_run_id: "run".into(),
            node_execution_id: "exec".into(),
            node_id: "plan".into(),
            agent_id: Some("planner".into()),
        },
        HookEvent::NodeEnd {
            topology_run_id: "run".into(),
            node_execution_id: "exec".into(),
            node_id: "plan".into(),
            success: true,
            output: Some(json!({"result": "ok"})),
        },
        HookEvent::FanOutSpawn {
            topology_run_id: "run".into(),
            source_node_execution_id: "exec".into(),
            edge_id: "edge".into(),
            branch_count: 2,
        },
        HookEvent::FanOutCollect {
            topology_run_id: "run".into(),
            source_node_execution_id: "exec".into(),
            edge_id: "edge".into(),
            branch_count: 2,
        },
    ];

    for event in &events {
        let outcome = hook.execute(event).await;
        assert!(
            matches!(outcome, HookOutcome::Continue),
            "expected Continue for {}",
            event.event_name()
        );
    }
}
