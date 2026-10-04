//! Streaming event types emitted by the agent during turn processing.
//!
//! The gateway (or any consumer) receives these events through a channel
//! and forwards them to connected clients. This decouples the agent's
//! execution loop from transport concerns.

use swage_core::{ApprovalTimeoutPolicy, ProviderResponseWitness};
use uuid::Uuid;

use crate::approval::ApprovalDecisionValue;

pub use swage_protocol::client::websocket::SchemaValidationError;

/// Events emitted by the agent over a channel during streaming turn execution.
///
/// All variants are `Send + 'static`, making them safe to transport through
/// tokio unbounded channels from the agent to any number of consumers.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// The turn record has been created and processing is beginning.
    TurnStarted { turn_id: Uuid },

    /// Agent-side context compaction completed and its audit record is ready.
    CompactionApplied {
        turn_id: Uuid,
        record: swage_protocol::client::context::CompactionRecord,
    },

    /// A streaming token arrived from the LLM.
    Token { content: String },

    /// The provider retry decorator is waiting before the next stream attempt.
    ProviderRetryStarted {
        turn_id: Uuid,
        attempt: u32,
        max_retries: u32,
        provider: String,
        wait_ms: u64,
    },

    /// A provider retry wait has settled.
    ProviderRetrySettled {
        turn_id: Uuid,
        attempt: u32,
        max_retries: u32,
        provider: String,
        success: bool,
    },

    /// A provider-visible request body has been prepared for a model call.
    ///
    /// This is diagnostic data for context snapshots. It is emitted before the
    /// provider stream is opened and after agent-side context compaction or
    /// tool-round replay has shaped the request.
    ProviderRequestPrepared {
        turn_id: Uuid,
        round: usize,
        request: serde_json::Value,
    },

    /// A provider request round has ended, with what came back.
    ///
    /// Paired with `ProviderRequestPrepared`: the two bracket one request to
    /// the provider, so a round that never settles is an outstanding request
    /// rather than a silent gap in the record.
    ProviderRoundSettled {
        turn_id: Uuid,
        round: usize,
        outcome: ProviderRoundOutcome,
    },

    /// Content-free evidence describing a provider response attempt.
    ProviderResponseCaptured {
        turn_id: Uuid,
        round: usize,
        witness: ProviderResponseWitness,
    },

    /// Provider reasoning side-channel content was received.
    ///
    /// Consumers may persist this for diagnostics, but must not display or
    /// replay it as visible assistant text unless an explicit provider/model
    /// capability says that is required.
    ProviderReasoningCaptured {
        turn_id: Uuid,
        round: usize,
        content: String,
        details: Option<serde_json::Value>,
    },

    /// A tool call has started; consumers can show a "running…" indicator.
    ToolStart {
        call_id: String,
        tool_name: String,
        arguments: serde_json::Value,
        /// One-line headline from the tool's display hints; None means
        /// renderers use their generic fallback.
        display_summary: Option<String>,
    },

    /// A tool call has finished.
    ToolEnd {
        call_id: String,
        tool_name: String,
        result: serde_json::Value,
        /// "success" | "error" | "cancelled"
        status: String,
        /// Wall-clock time of the tool execution, if available.
        duration_ms: Option<u64>,
    },

    /// The turn has completed successfully; token counts are final.
    TurnCompleted {
        turn_id: Uuid,
        input_tokens: u32,
        output_tokens: u32,
        tool_calls_count: u32,
        /// Parsed, validated JSON when the agent has an output schema and
        /// validation passes. None when no schema is configured or when
        /// validation failed after retry exhaustion.
        structured_output: Option<serde_json::Value>,
    },

    /// A schema validation attempt failed; the agent will retry the LLM call.
    ///
    /// Emitted once per failed attempt before the retry. Consumers can display
    /// retry progress or accumulate errors for diagnostics.
    OutputSchemaRetry {
        turn_id: Uuid,
        /// 1-indexed attempt number (first retry = 1).
        attempt: u32,
        /// Maximum configured retries for this turn.
        max_retries: u32,
        /// Validation errors from this attempt.
        errors: Vec<SchemaValidationError>,
    },

    /// All schema validation retries were exhausted without a valid response.
    ///
    /// Emitted once when the retry budget is spent. The turn still completes
    /// (with `structured_output: None` on `TurnCompleted`), but the caller
    /// should surface this to the user as a partial failure.
    OutputSchemaFailed {
        turn_id: Uuid,
        /// Total number of attempts made (initial + retries).
        attempts: u32,
        /// Validation errors from the final attempt.
        errors: Vec<SchemaValidationError>,
    },

    /// A tool or hook execution requires human approval before proceeding.
    ///
    /// The agent pauses the turn and waits for an external decision (approve,
    /// reject, or timeout). Consumers forward this to the UI so a human can
    /// review and decide.
    ApprovalRequired {
        approval_id: Uuid,
        turn_id: Uuid,
        agent_id: String,
        /// "tool" or "hook" — what triggered the approval gate.
        kind: String,
        /// Present when `kind` is "tool".
        tool_name: Option<String>,
        /// Present when `kind` is "tool".
        tool_arguments: Option<serde_json::Value>,
        /// Present when `kind` is "hook".
        hook_name: Option<String>,
        /// Human-readable description of what is being requested.
        prompt: String,
        /// Arbitrary metadata for the UI (e.g. environment, risk level).
        context: Option<serde_json::Value>,
        /// ISO 8601 timestamp after which the request expires.
        timeout_at: String,
        /// What happens on expiry.
        timeout_policy: ApprovalTimeoutPolicy,
    },

    /// A pending approval was resolved by a human decision.
    ApprovalConfirmed {
        approval_id: Uuid,
        /// Human decision.
        decision: ApprovalDecisionValue,
        /// Optional explanation from the reviewer.
        reason: Option<String>,
    },

    /// A pending approval expired without a decision.
    ApprovalExpired {
        approval_id: Uuid,
        /// The policy that was applied.
        timeout_policy: ApprovalTimeoutPolicy,
    },

    /// A tool-as-delegation has started — the parent agent's turn is
    /// suspended while the target agent executes.
    DelegationStarted {
        /// The agent that is being delegated to.
        target_agent_id: String,
        /// The task description sent to the delegate.
        task: String,
    },

    /// A tool-as-delegation has completed — the parent turn will resume
    /// with the delegate's result as the tool output.
    DelegationCompleted {
        /// The agent that was delegated to.
        target_agent_id: String,
        /// Whether the delegation succeeded.
        success: bool,
    },

    /// The provider explicitly refused the request.
    ProviderRefused { turn_id: Uuid },

    /// A non-recoverable error occurred during turn processing.
    Error {
        message: String,
        provider_failure: bool,
    },

    /// A constitutive turn record could not be written. The coordinator owns
    /// the Failed transition after the agent has settled.
    RecordIntegrityLost { turn_id: Uuid, reason: String },
}

/// How one provider request round ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderRoundOutcome {
    /// A response arrived, asking for this many tool calls. Zero means the
    /// model answered rather than calling tools.
    Responded { tool_calls: u32 },
    /// The round ended without a usable response.
    Failed { message: String },
    /// The caller stopped the round before a response could arrive.
    Interrupted,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_turn_started_constructible() {
        let turn_id = Uuid::new_v4();
        let event = AgentEvent::TurnStarted { turn_id };
        if let AgentEvent::TurnStarted { turn_id: id } = event {
            assert_eq!(id, turn_id);
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn test_token_constructible() {
        let event = AgentEvent::Token {
            content: "hello".to_string(),
        };
        if let AgentEvent::Token { content } = event {
            assert_eq!(content, "hello");
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn test_tool_start_constructible() {
        let event = AgentEvent::ToolStart {
            call_id: "call-1".to_string(),
            tool_name: "bash".to_string(),
            arguments: serde_json::json!({"cmd": "ls"}),
            display_summary: Some("ls".to_string()),
        };
        if let AgentEvent::ToolStart {
            call_id,
            tool_name,
            arguments,
            display_summary,
        } = event
        {
            assert_eq!(call_id, "call-1");
            assert_eq!(tool_name, "bash");
            assert_eq!(arguments["cmd"], "ls");
            assert_eq!(display_summary.as_deref(), Some("ls"));
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn test_tool_end_constructible() {
        let event = AgentEvent::ToolEnd {
            call_id: "call-1".to_string(),
            tool_name: "bash".to_string(),
            result: serde_json::json!({"output": "file.txt"}),
            status: "success".to_string(),
            duration_ms: Some(42),
        };
        if let AgentEvent::ToolEnd {
            call_id,
            tool_name,
            result,
            status,
            duration_ms,
        } = event
        {
            assert_eq!(call_id, "call-1");
            assert_eq!(tool_name, "bash");
            assert_eq!(result["output"], "file.txt");
            assert_eq!(status, "success");
            assert_eq!(duration_ms, Some(42));
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn test_tool_end_no_duration() {
        let event = AgentEvent::ToolEnd {
            call_id: "call-2".to_string(),
            tool_name: "read_file".to_string(),
            result: serde_json::Value::Null,
            status: "error".to_string(),
            duration_ms: None,
        };
        if let AgentEvent::ToolEnd { duration_ms, .. } = event {
            assert!(duration_ms.is_none());
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn test_turn_completed_constructible() {
        let turn_id = Uuid::new_v4();
        let event = AgentEvent::TurnCompleted {
            turn_id,
            input_tokens: 100,
            output_tokens: 200,
            tool_calls_count: 3,
            structured_output: None,
        };
        if let AgentEvent::TurnCompleted {
            turn_id: id,
            input_tokens,
            output_tokens,
            tool_calls_count,
            structured_output,
        } = event
        {
            assert_eq!(id, turn_id);
            assert_eq!(input_tokens, 100);
            assert_eq!(output_tokens, 200);
            assert_eq!(tool_calls_count, 3);
            assert!(structured_output.is_none());
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn test_error_constructible() {
        let event = AgentEvent::Error {
            message: "something went wrong".to_string(),
            provider_failure: false,
        };
        if let AgentEvent::Error {
            message,
            provider_failure,
        } = event
        {
            assert_eq!(message, "something went wrong");
            assert!(!provider_failure);
        } else {
            panic!("unexpected variant");
        }
    }

    #[tokio::test]
    async fn test_channel_send() {
        // Verify all variants can traverse a tokio unbounded channel (Send + 'static).
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
        let turn_id = Uuid::new_v4();

        tx.send(AgentEvent::TurnStarted { turn_id }).unwrap();
        tx.send(AgentEvent::Token {
            content: "tok".to_string(),
        })
        .unwrap();
        tx.send(AgentEvent::ToolStart {
            call_id: "c".to_string(),
            tool_name: "t".to_string(),
            arguments: serde_json::Value::Null,
            display_summary: None,
        })
        .unwrap();
        tx.send(AgentEvent::ToolEnd {
            call_id: "c".to_string(),
            tool_name: "t".to_string(),
            result: serde_json::Value::Null,
            status: "success".to_string(),
            duration_ms: None,
        })
        .unwrap();
        tx.send(AgentEvent::TurnCompleted {
            turn_id,
            input_tokens: 1,
            output_tokens: 2,
            tool_calls_count: 0,
            structured_output: None,
        })
        .unwrap();
        tx.send(AgentEvent::ApprovalRequired {
            approval_id: Uuid::new_v4(),
            turn_id,
            agent_id: "a".to_string(),
            kind: "tool".to_string(),
            tool_name: Some("bash".to_string()),
            tool_arguments: None,
            hook_name: None,
            prompt: "ok?".to_string(),
            context: None,
            timeout_at: "2026-01-01T00:00:00Z".to_string(),
            timeout_policy: ApprovalTimeoutPolicy::Reject,
        })
        .unwrap();
        tx.send(AgentEvent::ApprovalConfirmed {
            approval_id: Uuid::new_v4(),
            decision: ApprovalDecisionValue::Approved,
            reason: None,
        })
        .unwrap();
        tx.send(AgentEvent::ApprovalExpired {
            approval_id: Uuid::new_v4(),
            timeout_policy: ApprovalTimeoutPolicy::Interrupt,
        })
        .unwrap();
        tx.send(AgentEvent::DelegationStarted {
            target_agent_id: "reviewer".to_string(),
            task: "Review code".to_string(),
        })
        .unwrap();
        tx.send(AgentEvent::DelegationCompleted {
            target_agent_id: "reviewer".to_string(),
            success: true,
        })
        .unwrap();
        tx.send(AgentEvent::Error {
            message: "err".to_string(),
            provider_failure: false,
        })
        .unwrap();

        // Drain the channel — all 11 events must arrive.
        let mut received = 0;
        drop(tx);
        while rx.recv().await.is_some() {
            received += 1;
        }
        assert_eq!(received, 11);
    }

    #[tokio::test]
    async fn test_structured_output_round_trips_through_channel() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
        let turn_id = Uuid::new_v4();
        let payload = serde_json::json!({"x": 1});

        tx.send(AgentEvent::TurnCompleted {
            turn_id,
            input_tokens: 10,
            output_tokens: 20,
            tool_calls_count: 0,
            structured_output: Some(payload.clone()),
        })
        .unwrap();
        drop(tx);

        let event = rx.recv().await.expect("should receive event");
        if let AgentEvent::TurnCompleted {
            structured_output, ..
        } = event
        {
            assert_eq!(structured_output, Some(payload));
        } else {
            panic!("expected TurnCompleted");
        }
    }

    #[test]
    fn test_debug_and_clone() {
        let event = AgentEvent::Token {
            content: "x".to_string(),
        };
        let cloned = event.clone();
        let debug_str = format!("{cloned:?}");
        assert!(debug_str.contains("Token"));
        assert!(matches!(event, AgentEvent::Token { .. }));
    }

    // -----------------------------------------------------------------------
    // SchemaValidationError [vd37]
    // -----------------------------------------------------------------------

    #[test]
    fn schema_validation_error_constructible() {
        let err = SchemaValidationError {
            path: "/properties/name".to_string(),
            message: "required".to_string(),
        };
        assert_eq!(err.path, "/properties/name");
        assert_eq!(err.message, "required");
    }

    #[test]
    fn schema_validation_error_serialises() {
        let err = SchemaValidationError {
            path: "/count".to_string(),
            message: "must be >= 0".to_string(),
        };
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["path"], "/count");
        assert_eq!(json["message"], "must be >= 0");
    }

    #[test]
    fn schema_validation_error_debug_and_clone() {
        let err = SchemaValidationError {
            path: "/x".to_string(),
            message: "bad".to_string(),
        };
        let cloned = err.clone();
        let debug_str = format!("{cloned:?}");
        assert!(debug_str.contains("SchemaValidationError"));
        assert_eq!(err.path, "/x");
    }

    // -----------------------------------------------------------------------
    // OutputSchemaRetry [vd37]
    // -----------------------------------------------------------------------

    #[test]
    fn output_schema_retry_constructible() {
        let turn_id = Uuid::new_v4();
        let event = AgentEvent::OutputSchemaRetry {
            turn_id,
            attempt: 1,
            max_retries: 3,
            errors: vec![SchemaValidationError {
                path: "/name".to_string(),
                message: "required".to_string(),
            }],
        };
        if let AgentEvent::OutputSchemaRetry {
            turn_id: id,
            attempt,
            max_retries,
            errors,
        } = event
        {
            assert_eq!(id, turn_id);
            assert_eq!(attempt, 1);
            assert_eq!(max_retries, 3);
            assert_eq!(errors.len(), 1);
            assert_eq!(errors[0].path, "/name");
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn output_schema_retry_debug_and_clone() {
        let event = AgentEvent::OutputSchemaRetry {
            turn_id: Uuid::new_v4(),
            attempt: 1,
            max_retries: 3,
            errors: vec![],
        };
        let cloned = event.clone();
        let debug_str = format!("{cloned:?}");
        assert!(debug_str.contains("OutputSchemaRetry"));
        assert!(matches!(event, AgentEvent::OutputSchemaRetry { .. }));
    }

    // -----------------------------------------------------------------------
    // OutputSchemaFailed [vd37]
    // -----------------------------------------------------------------------

    #[test]
    fn output_schema_failed_constructible() {
        let turn_id = Uuid::new_v4();
        let event = AgentEvent::OutputSchemaFailed {
            turn_id,
            attempts: 3,
            errors: vec![SchemaValidationError {
                path: "/name".to_string(),
                message: "required".to_string(),
            }],
        };
        if let AgentEvent::OutputSchemaFailed {
            turn_id: id,
            attempts,
            errors,
        } = event
        {
            assert_eq!(id, turn_id);
            assert_eq!(attempts, 3);
            assert_eq!(errors.len(), 1);
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn output_schema_failed_debug_and_clone() {
        let event = AgentEvent::OutputSchemaFailed {
            turn_id: Uuid::new_v4(),
            attempts: 3,
            errors: vec![],
        };
        let cloned = event.clone();
        let debug_str = format!("{cloned:?}");
        assert!(debug_str.contains("OutputSchemaFailed"));
        assert!(matches!(event, AgentEvent::OutputSchemaFailed { .. }));
    }

    // -----------------------------------------------------------------------
    // Approval lifecycle variants [bm2t]
    // -----------------------------------------------------------------------

    #[test]
    fn approval_required_constructible() {
        let approval_id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();
        let event = AgentEvent::ApprovalRequired {
            approval_id,
            turn_id,
            agent_id: "agent-1".to_string(),
            kind: "tool".to_string(),
            tool_name: Some("bash".to_string()),
            tool_arguments: Some(serde_json::json!({"cmd": "rm -rf /"})),
            hook_name: None,
            prompt: "Allow destructive command?".to_string(),
            context: None,
            timeout_at: "2026-03-24T12:00:00Z".to_string(),
            timeout_policy: ApprovalTimeoutPolicy::Reject,
        };
        if let AgentEvent::ApprovalRequired {
            approval_id: aid,
            turn_id: tid,
            agent_id,
            kind,
            tool_name,
            tool_arguments,
            hook_name,
            prompt,
            context,
            timeout_at,
            timeout_policy,
        } = event
        {
            assert_eq!(aid, approval_id);
            assert_eq!(tid, turn_id);
            assert_eq!(agent_id, "agent-1");
            assert_eq!(kind, "tool");
            assert_eq!(tool_name.as_deref(), Some("bash"));
            assert!(tool_arguments.is_some());
            assert!(hook_name.is_none());
            assert_eq!(prompt, "Allow destructive command?");
            assert!(context.is_none());
            assert_eq!(timeout_at, "2026-03-24T12:00:00Z");
            assert_eq!(timeout_policy, ApprovalTimeoutPolicy::Reject);
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn approval_required_hook_kind() {
        // Verify the hook-triggered flavour with hook_name populated.
        let event = AgentEvent::ApprovalRequired {
            approval_id: Uuid::new_v4(),
            turn_id: Uuid::new_v4(),
            agent_id: "agent-2".to_string(),
            kind: "hook".to_string(),
            tool_name: None,
            tool_arguments: None,
            hook_name: Some("pre_deploy".to_string()),
            prompt: "Deploy to production?".to_string(),
            context: Some(serde_json::json!({"env": "prod"})),
            timeout_at: "2026-03-24T13:00:00Z".to_string(),
            timeout_policy: ApprovalTimeoutPolicy::Interrupt,
        };
        if let AgentEvent::ApprovalRequired {
            kind,
            hook_name,
            context,
            timeout_policy,
            ..
        } = event
        {
            assert_eq!(kind, "hook");
            assert_eq!(hook_name.as_deref(), Some("pre_deploy"));
            assert!(context.is_some());
            assert_eq!(timeout_policy, ApprovalTimeoutPolicy::Interrupt);
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn approval_confirmed_constructible() {
        let approval_id = Uuid::new_v4();
        let event = AgentEvent::ApprovalConfirmed {
            approval_id,
            decision: ApprovalDecisionValue::Approved,
            reason: Some("Looks safe".to_string()),
        };
        if let AgentEvent::ApprovalConfirmed {
            approval_id: aid,
            decision,
            reason,
        } = event
        {
            assert_eq!(aid, approval_id);
            assert_eq!(decision, ApprovalDecisionValue::Approved);
            assert_eq!(reason.as_deref(), Some("Looks safe"));
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn approval_confirmed_no_reason() {
        let event = AgentEvent::ApprovalConfirmed {
            approval_id: Uuid::new_v4(),
            decision: ApprovalDecisionValue::Rejected,
            reason: None,
        };
        if let AgentEvent::ApprovalConfirmed { reason, .. } = event {
            assert!(reason.is_none());
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn approval_expired_constructible() {
        let approval_id = Uuid::new_v4();
        let event = AgentEvent::ApprovalExpired {
            approval_id,
            timeout_policy: ApprovalTimeoutPolicy::Reject,
        };
        if let AgentEvent::ApprovalExpired {
            approval_id: aid,
            timeout_policy,
        } = event
        {
            assert_eq!(aid, approval_id);
            assert_eq!(timeout_policy, ApprovalTimeoutPolicy::Reject);
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn approval_events_debug_and_clone() {
        let required = AgentEvent::ApprovalRequired {
            approval_id: Uuid::new_v4(),
            turn_id: Uuid::new_v4(),
            agent_id: "a".to_string(),
            kind: "tool".to_string(),
            tool_name: None,
            tool_arguments: None,
            hook_name: None,
            prompt: "ok?".to_string(),
            context: None,
            timeout_at: "2026-01-01T00:00:00Z".to_string(),
            timeout_policy: ApprovalTimeoutPolicy::Reject,
        };
        let debug_str = format!("{required:?}");
        assert!(debug_str.contains("ApprovalRequired"));

        let confirmed = AgentEvent::ApprovalConfirmed {
            approval_id: Uuid::new_v4(),
            decision: ApprovalDecisionValue::Approved,
            reason: None,
        };
        let debug_str = format!("{confirmed:?}");
        assert!(debug_str.contains("ApprovalConfirmed"));

        let expired = AgentEvent::ApprovalExpired {
            approval_id: Uuid::new_v4(),
            timeout_policy: ApprovalTimeoutPolicy::Interrupt,
        };
        let debug_str = format!("{expired:?}");
        assert!(debug_str.contains("ApprovalExpired"));
    }

    #[tokio::test]
    async fn approval_events_channel_sendable() {
        // Verify all three approval variants traverse a tokio unbounded channel.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();

        tx.send(AgentEvent::ApprovalRequired {
            approval_id: Uuid::new_v4(),
            turn_id: Uuid::new_v4(),
            agent_id: "a".to_string(),
            kind: "tool".to_string(),
            tool_name: Some("bash".to_string()),
            tool_arguments: None,
            hook_name: None,
            prompt: "allow?".to_string(),
            context: None,
            timeout_at: "2026-01-01T00:00:00Z".to_string(),
            timeout_policy: ApprovalTimeoutPolicy::Reject,
        })
        .unwrap();
        tx.send(AgentEvent::ApprovalConfirmed {
            approval_id: Uuid::new_v4(),
            decision: ApprovalDecisionValue::Approved,
            reason: Some("ok".to_string()),
        })
        .unwrap();
        tx.send(AgentEvent::ApprovalExpired {
            approval_id: Uuid::new_v4(),
            timeout_policy: ApprovalTimeoutPolicy::Interrupt,
        })
        .unwrap();
        drop(tx);

        let mut received = 0;
        while rx.recv().await.is_some() {
            received += 1;
        }
        assert_eq!(received, 3);
    }

    #[tokio::test]
    async fn output_schema_events_channel_sendable() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
        let turn_id = Uuid::new_v4();

        tx.send(AgentEvent::OutputSchemaRetry {
            turn_id,
            attempt: 1,
            max_retries: 3,
            errors: vec![SchemaValidationError {
                path: "/x".to_string(),
                message: "bad".to_string(),
            }],
        })
        .unwrap();
        tx.send(AgentEvent::OutputSchemaFailed {
            turn_id,
            attempts: 3,
            errors: vec![],
        })
        .unwrap();
        drop(tx);

        let mut received = 0;
        while rx.recv().await.is_some() {
            received += 1;
        }
        assert_eq!(received, 2);
    }
}
