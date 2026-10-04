use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

use swage_core::{GatewayError, StorageError};
use swage_storage::NewTurn;

use crate::state::AppState;
use crate::subscription::SubscriptionState;
use crate::thread_selection::resolve_thread_for_session;

/// Map a placeholder-turn insertion failure to a gateway error.
///
/// The turns table carries a partial unique index admitting one non-terminal
/// turn per thread. The in-memory pre-check below catches the common case,
/// but two automated runs racing on a shared continuation session can both
/// pass it and collide on the insert. That collision is an overlap, not an
/// internal fault, so it becomes `TurnInProgress` — never a raw
/// `UNIQUE constraint failed: turns.thread_id` surfaced to a caller or a
/// run record. This mirrors the interactive turn path's
/// `map_turn_creation_error`.
fn map_placeholder_turn_creation_error(error: StorageError, session_id: Uuid) -> GatewayError {
    match error {
        StorageError::ConstraintViolation(_) => GatewayError::TurnInProgress { session_id },
        other => GatewayError::Storage(other),
    }
}

pub async fn create_placeholder_turn(
    state: &AppState,
    session_id: Uuid,
    agent_id: &str,
    message: &str,
) -> Result<Uuid, GatewayError> {
    let thread = resolve_thread_for_session(state.storage.as_ref(), session_id, None).await?;
    let existing_turns = state.storage.list_turns_by_thread(thread.id).await?;
    if existing_turns.iter().any(|turn| !turn.state.is_terminal())
        || state.active_turns.lock().await.contains_key(&session_id)
    {
        return Err(GatewayError::TurnInProgress { session_id });
    }

    let turn = state
        .storage
        .create_turn(NewTurn {
            thread_id: thread.id,
            agent_id: agent_id.to_string(),
            state: swage_storage::TurnState::Pending,
            user_input: Some(message.to_string()),
            parent_turn_id: None,
        })
        .await
        .map_err(|error| map_placeholder_turn_creation_error(error, session_id))?;
    Ok(turn.id)
}

pub async fn run_placeholder_turn(
    state: &AppState,
    session_id: Uuid,
    input: impl Into<crate::turn::TurnInput>,
    turn_id: Uuid,
) -> Result<String, GatewayError> {
    let input: crate::turn::TurnInput = input.into();
    let thread = resolve_thread_for_session(state.storage.as_ref(), session_id, None).await?;
    let (ws_tx, mut ws_rx) = mpsc::unbounded_channel();
    let subscription = Arc::new(Mutex::new(SubscriptionState::new(false)));
    tokio::spawn(async move { while ws_rx.recv().await.is_some() {} });

    Box::pin(crate::turn::process_turn_with_existing_turn(
        state,
        session_id,
        Some(thread.id),
        input,
        ws_tx,
        subscription,
        None,
        None,
        None,
        None,
        Some(turn_id),
        crate::turn::TurnAttendance::Unattended,
    ))
    .await?;

    let turn = state.storage.get_turn(turn_id).await?.ok_or_else(|| {
        GatewayError::ValidationError(format!(
            "turn {turn_id} not found after automated execution"
        ))
    })?;
    turn.assistant_response.ok_or_else(|| {
        GatewayError::ValidationError("automated turn completed without assistant response".into())
    })
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use swage_core::{FinishReason, StreamChunk};

    use super::*;
    use crate::turn::tests::{
        make_state_with_approval_gated_echo, make_state_with_config, make_state_with_session,
    };

    #[test]
    fn placeholder_turn_constraint_maps_to_turn_in_progress() {
        // A raw turns-table uniqueness violation must never surface as such —
        // it is the atomic overlap guard firing, so it reads as an in-flight
        // turn, not an internal storage fault.
        let session_id = Uuid::new_v4();
        let mapped = map_placeholder_turn_creation_error(
            StorageError::ConstraintViolation("UNIQUE constraint failed: turns.thread_id".into()),
            session_id,
        );
        assert!(matches!(
            mapped,
            GatewayError::TurnInProgress { session_id: id } if id == session_id
        ));

        // Other storage errors pass through untouched.
        let mapped = map_placeholder_turn_creation_error(
            StorageError::QueryFailed("disk gone".into()),
            session_id,
        );
        assert!(matches!(mapped, GatewayError::Storage(_)));
    }

    #[tokio::test]
    async fn automated_placeholder_turn_receives_provider_visible_snapshot() {
        let (state, session_id) = make_state_with_session(vec![
            StreamChunk::Delta {
                content: "Automated answer".into(),
            },
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ])
        .await;

        let turn_id = create_placeholder_turn(&state, session_id, "gateway", "Automated hello")
            .await
            .expect("create automated placeholder");
        let response = run_placeholder_turn(&state, session_id, "Automated hello", turn_id)
            .await
            .expect("automated turn should complete");

        assert_eq!(response, "Automated answer");
        let snapshot = state
            .storage
            .get_snapshot(turn_id)
            .await
            .expect("get snapshot")
            .expect("automated placeholder should receive context snapshot");
        let parsed: Value = serde_json::from_str(&snapshot).expect("snapshot should be JSON");
        let provider_request = &parsed["provider_visible"]["requests"][0];
        assert_eq!(provider_request["round"], serde_json::json!(1));
        assert!(
            provider_request["request"]["body"]["messages"].is_array(),
            "automated turn snapshot should include provider-visible messages"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_turn_records_the_boundary_its_dispatcher_settled_at() {
        let reply = || {
            vec![
                StreamChunk::Delta {
                    content: "Done.".into(),
                },
                StreamChunk::Done {
                    finish_reason: Some(FinishReason::Stop),
                },
            ]
        };

        // On a host: the test harness stands in for the sandbox.
        let (state, session_id) = make_state_with_session(reply()).await;
        let turn_id = create_placeholder_turn(&state, session_id, "gateway", "Hello")
            .await
            .unwrap();
        run_placeholder_turn(&state, session_id, "Hello", turn_id)
            .await
            .unwrap();
        let turn = state.storage.get_turn(turn_id).await.unwrap().unwrap();
        assert_eq!(
            turn.execution_isolation,
            Some(swage_storage::TurnIsolation::Isolated),
            "every settled turn carries where it ran"
        );

        // In an install that declares its boundary: the turn says so, and
        // nothing about it reads as degraded.
        let (mut state, session_id) = make_state_with_session(reply()).await;
        state.declared_execution_boundary = Some("container".into());
        let turn_id = create_placeholder_turn(&state, session_id, "gateway", "Hello")
            .await
            .unwrap();
        run_placeholder_turn(&state, session_id, "Hello", turn_id)
            .await
            .unwrap();
        let turn = state.storage.get_turn(turn_id).await.unwrap().unwrap();
        assert_eq!(
            turn.execution_isolation,
            Some(swage_storage::TurnIsolation::Declared {
                boundary: "container".into()
            })
        );
        assert!(
            state
                .storage
                .list_audit_events()
                .await
                .unwrap()
                .iter()
                .all(|event| event.event_kind != "execution_isolation_degraded"),
            "a declared boundary is not a degradation"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_scheduled_brief_is_recorded_by_its_origin_and_announced_to_open_views() {
        // The brief rides the user role to the model, so the record must say
        // a person did not type it — and an open view of the continued
        // conversation learns of it live, never only on reload.
        let (state, session_id) = make_state_with_session(vec![
            StreamChunk::Delta {
                content: "Inbox summarised".into(),
            },
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ])
        .await;
        let mut announcements = state.broadcast.subscribe();

        let turn_id = create_placeholder_turn(&state, session_id, "gateway", "Summarise the inbox")
            .await
            .expect("create scheduled placeholder");
        run_placeholder_turn(
            &state,
            session_id,
            crate::turn::TurnInput::text("Summarise the inbox")
                .with_origin(swage_storage::MessageOrigin::Schedule),
            turn_id,
        )
        .await
        .expect("scheduled turn should complete");

        let thread = resolve_thread_for_session(state.storage.as_ref(), session_id, None)
            .await
            .expect("thread");
        let messages = state
            .storage
            .list_messages_by_thread(thread.id)
            .await
            .expect("messages");
        let origins: Vec<_> = messages
            .iter()
            .map(|message| (message.role, message.origin))
            .collect();
        assert_eq!(
            origins,
            vec![
                (
                    swage_core::Role::User,
                    swage_storage::MessageOrigin::Schedule
                ),
                (
                    swage_core::Role::Assistant,
                    swage_storage::MessageOrigin::Assistant
                ),
            ]
        );

        let announced = announcements
            .try_recv()
            .expect("the brief is announced on the broadcast");
        match announced {
            swage_protocol::client::websocket::ServerEvent::MessageAppended {
                session_id: announced_session,
                message,
            } => {
                assert_eq!(announced_session, session_id);
                assert_eq!(
                    message.id, messages[0].id,
                    "the announced view is the recorded row"
                );
                assert_eq!(message.origin, swage_storage::MessageOrigin::Schedule);
                assert_eq!(message.content, "Summarise the inbox");
            }
            other => panic!("expected the brief's announcement, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_typed_message_is_not_announced_to_its_own_author() {
        // The person's view already holds what they typed; announcing it
        // would show it twice. Only a row they did not write is announced.
        let (state, session_id) = make_state_with_session(vec![
            StreamChunk::Delta {
                content: "Hello back".into(),
            },
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ])
        .await;
        let mut announcements = state.broadcast.subscribe();

        let turn_id = create_placeholder_turn(&state, session_id, "gateway", "Hello")
            .await
            .expect("create placeholder");
        run_placeholder_turn(&state, session_id, "Hello", turn_id)
            .await
            .expect("turn should complete");

        assert!(
            !matches!(
                announcements.try_recv(),
                Ok(swage_protocol::client::websocket::ServerEvent::MessageAppended { .. })
            ),
            "a person's own message is never announced back to them"
        );
    }

    #[tokio::test]
    async fn scheduled_turn_executes_granted_approval_gated_tool_without_parking() {
        let rounds = vec![
            vec![
                StreamChunk::ToolUse {
                    call_id: "scheduled-echo".into(),
                    name: "echo".into(),
                    arguments: serde_json::json!({ "text": "scheduled" }),
                },
                StreamChunk::Done {
                    finish_reason: None,
                },
            ],
            vec![
                StreamChunk::Delta {
                    content: "Scheduled tool completed".into(),
                },
                StreamChunk::Done {
                    finish_reason: None,
                },
            ],
        ];
        let (state, session_id) = make_state_with_approval_gated_echo(rounds).await;
        let turn_id = create_placeholder_turn(&state, session_id, "gateway", "Use echo")
            .await
            .expect("create scheduled placeholder");

        let response = run_placeholder_turn(&state, session_id, "Use echo", turn_id)
            .await
            .expect("scheduled turn should execute without an approval answer");

        assert_eq!(response, "Scheduled tool completed");
        assert!(
            state.storage.list_approvals().await.unwrap().is_empty(),
            "an unattended scheduled turn must not create approval rows"
        );
        let calls = state
            .storage
            .list_tool_calls_by_turn(turn_id)
            .await
            .unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].status.as_deref(), Some("success"));
        assert_eq!(
            calls[0].result.as_deref(),
            Some("{\"output\":\"scheduled\"}")
        );
    }

    #[tokio::test]
    async fn unattended_turn_respects_explicit_sandbox_disablement_and_records_degradation() {
        let mut config = swage_config::ResolvedConfig::default();
        config.sandbox.enabled = false;
        let (state, session_id) = make_state_with_config(
            vec![
                StreamChunk::Delta {
                    content: "Explicitly unsandboxed".into(),
                },
                StreamChunk::Done {
                    finish_reason: Some(swage_core::FinishReason::Stop),
                },
            ],
            config,
        )
        .await;
        let turn_id = create_placeholder_turn(&state, session_id, "gateway", "Run unsandboxed")
            .await
            .expect("create automated placeholder");

        let response = run_placeholder_turn(&state, session_id, "Run unsandboxed", turn_id)
            .await
            .expect("explicit sandbox disablement should apply to unattended execution");
        assert_eq!(response, "Explicitly unsandboxed");

        let turn = state.storage.get_turn(turn_id).await.unwrap().unwrap();
        assert_eq!(turn.state, swage_storage::TurnState::Completed);
        let events = state.storage.list_audit_events().await.unwrap();
        assert!(events.iter().any(|event| {
            event.entity_id == turn_id.to_string()
                && event.event_kind == "execution_isolation_degraded"
                && event.metadata["reason"] == "sandbox_disabled_by_configuration"
        }));
    }
}
